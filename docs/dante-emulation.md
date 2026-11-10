# Device emulation: a Dante device that isn't there

> ## Status: built from a capture, never run against a console
>
> The bridge can put a **virtual Yamaha Rio3224-D2** on a Dante network. A CL
> or QL console should see a stagebox it knows, mount it, and drive its head
> amps with its own gain and +48 V controls. Each move arrives at the bridge,
> where a `[[mapping]]` sends it on to hardware the console has never heard
> of: another maker's stagebox, a radio-mic receiver's output level, an audio
> interface's mixer.
>
> "Should" is the honest word. Every answer the virtual Rio gives was checked
> against what a real Rio3224-D2 answered a real QL1, out of one packet
> capture (§4). **No console has been put in front of it yet.** §5 is the
> list of things only a console can settle, in the order to try them.

Everything else in this project connects to a device that exists and drives
it. This is the other way round: the bridge *is* the device.

---

## 1. Try it

```toml
[[device]]
id = "virtual-rio"
kind = "yamaha-rio-emulated"
address = "169.254.10.20"   # THIS machine's address on the Dante network
unit_id = 5                 # the console mounts it as Y005

# The console's gain on virtual input 3 drives a TotalMix output.
[[mapping]]
from = { device = "virtual-rio", channel = 3 }
to   = { device = "digiface", channel = 1 }
gain_offset_db = -60.0
```

`address` is not somewhere to dial. It is the local interface the device
appears on, so it has to be this machine's own address on the Dante network.
The device comes up as `Y005-Yamaha-Rio3224-D2-xxxxxx`, the name format a
Yamaha console recognises a Rio by.

| Key | Default | |
|---|---|---|
| `unit_id` | required | The stagebox unit id, 1 to 4095; `0x1F` style is accepted. **It must not clash with a real unit on the network.** |
| `model` | `Rio3224-D2` | The only model there is captured evidence for. Anything else is refused. |
| `name` | built | Dante device name. Built as `Y<unit>-Yamaha-<model>-<last three MAC bytes>`. |
| `mac` | derived | The MAC the device claims, e.g. `02:bb:5a:01:02:03`. Left out, a locally administered address is derived from the device id and `address`, the same on every run. |
| `advertise` | `true` | Announce the device over mDNS. |
| `presence` | `true` | Answer Dante's own queries (ARC, ConMon). Off, only the head-amp protocol runs. |
| `meters` | `true` | Stream input meters (silent ones) while a console asks for them. |
| `arc_port`, `cmc_port`, `control_port` | 4440, 8800, 8700 | `0` lets the system choose. A standard port that is already taken is replaced the same way. |

It has 32 channels, each with gain (−6 to +66 dB in whole dB, a Rio's range
and step) and +48 V. There is no pad: which of the Rio's arrays carries it is
still unknown, and the adapter reports `None` instead of guessing.

Add it in `bridge.toml`. The patch-bay web UI's add-device form has no field for
`unit_id` (or any other per-kind setting) yet, so it cannot create one; once the
bridge is running the device shows there and maps like any other.

For a first session with a real console, run it on its own. A plugin's log
lines do not reach `preamp-bridge`'s log; this prints everything:

```bash
cargo run -p dante-babelbox-preamp-adapter-yamaha --example emulated_rio -- 169.254.10.20 5
```

## 2. Before it goes near a show

- **The console overwrites it on pairing.** When a console finds a stagebox
  it writes its own stored gain and +48 V to all 32 channels, whatever the
  stagebox held. Those writes are relayed like any others. The moment the
  console mounts the virtual Rio, everything mapped from it takes the
  console's scene values, **phantom power included**. That is the point, and
  it is exactly what a real Rio gets, but map with it in mind.
- **Pick a free unit id.** A console mounts stageboxes by unit id. Two
  devices answering to one id is a fault you would be creating.
- **It carries no audio.** See §6.
- **Dante Virtual Soundcard holds the standard ports.** On a machine running
  DVS, 4440 and 8800 are taken. The virtual device then takes whatever ports
  the system gives it and advertises those, which is how Dante finds any
  device's ports. Whether a console is happy with a stagebox off the standard
  ports is not known. For the first test, use a machine without DVS running.
- **Dante Controller will show it as not clock-synced.** It runs no clock.

## 3. What it puts on the wire

Two layers, kept apart in the code as they are on the network.

**A Dante device** (`crates/emulation`, vendor-neutral):

| | Port | What it does |
|---|---|---|
| mDNS | 5353 | Advertises `_netaudio-arc`, `_netaudio-cmc` (both also under the `Yamaha._sub` subtype), `_netaudio-dbc`, and one `_netaudio-chan` record per input, with a Rio's TXT records. |
| ARC | UDP 4440 | Answers a controller's questions: device name, channel counts and names, properties, what each output is subscribed to. Stores a subscription or a property a controller sets and reads it back. Answers both the console's generation of questions and Dante Controller's (§4). |
| ConMon registration | UDP 8800 | Tells a controller where the control port is, and takes its request to be sent the status channel. |
| ConMon status | UDP 8700 in, 224.0.0.231:8702 out | Publishes clock, versions, sample rate, encoding, interface and make-and-model status when asked, at start-up, and every few seconds. Follows a controller's change of sample encoding (the QL1 moves a Rio from 24 to 32 bit). |
| ConMon heartbeat | 224.0.0.233:8708 | Once a second. |

**A Yamaha stagebox's head amps** (`preamp-adapter-yamaha`, `mbc::device`),
on the ConMon vendor group, 224.0.0.232:8705. This is the `MBC` protocol of
[`yamaha-ha-remote-over-dante.md`](yamaha-ha-remote-over-dante.md), from the
stagebox's side:

| The console | The virtual Rio |
|---|---|
| broadcasts a poll (`071f/06`) every six seconds | answers with its unit id, model and Dante name |
| reads its identity, channel counts and every head-amp array | answers each with a Rio3224-D2's values |
| writes +48 V and gain, all 32 channels at a time | stores them, echoes them, and reports the channels that changed to the bridge |
| pushes its `0731` arrays | hands them back |
| asks for metering, about every ten seconds | acknowledges, then streams 31 frames a second (silence) until the asking stops |

When the bridge changes a value (a mapped device moved), the virtual Rio
announces the whole array to everyone, the way a Rio announces its own state.

Nothing it sends is spoofed. Every packet is ordinary UDP from this machine's
own address. The "MAC" above is an identity carried inside Dante's payloads,
and by default it is a locally administered address (`02:…`) that cannot be
a real maker's.

## 4. The evidence

One capture: a QL1 pairing with a Rio3224-D2, patching it, sweeping gain on
four inputs and switching +48 V. It lives in the private `dante-captures`
repo as `yamaha-ql1-rio3224d2/ql1-rio3224d2-pairing-gain-phantom.control.pcap`.

Two replay tools put the QL1's own packets to the emulator and compare every
answer with the Rio's, byte for byte:

```bash
cargo run -p dante-babelbox-preamp-adapter-yamaha --example replay_mbc_device -- <capture>
cargo run -p dante-babelbox-emulation --example replay_capture -- <capture>
```

| Layer | Identical to the Rio | What differs, and why |
|---|---|---|
| Head-amp protocol (`MBC`) | **237 of 237** | Nothing. Pairing, 70 gain writes, 9 phantom writes, 49 polls of `0722/14`, 11 metering requests. |
| ConMon registration | **147 of 147** | Nothing. |
| ARC | **48 of 64** | 9 receive-flow and 1 transmit-flow listings: the Rio had audio flows. 6 output listings: the Rio's subscriptions had connected, a virtual one's never do. |
| ConMon status | **13 of 16** | 2 clock statuses: the Rio had found a clock leader. 1 make-and-model: one undecoded word differs in the Rio's first second (see below). |

The replay starts the emulator in the state the captured Rio was in: the
+48 V it held from its last session, and the 24 output subscriptions it
already had. Everything else comes out of the emulator's own defaults.

### Dante Controller's questions

Dante Controller does not ask what a console asks. It speaks a later ARC
version (`0x2808`) and reads channels through a second generation of
listings (`0x2002`, `0x2400`, `0x3400`, with `0x2600` / `0x3600` for flows)
that the QL1 never used. The only capture of those is Dante Controller with
a **DM3's** Dante module (`yamaha-dm3/dm3-dante-cmc-arc.pcapng`), so that is
what the emulator's answers to them are built from: the module's layouts,
filled with the persona's channels.

Shaped like that module (16 in, 16 out, 96 kHz), the emulator's answers
compare with the module's like this:

```bash
cargo run -p dante-babelbox-emulation --example replay_capture -- <dm3-dante-cmc-arc.pcapng> \
    --device 169.254.237.144 --name Y001-Yamaha-DM3-a436af --mac ac:44:f2:a4:36:af \
    --tx 16 --rx 16 --rate 96000 --property 8205=0007a120 --cold
```

| | Identical to the DM3's module | |
|---|---|---|
| Channel, label and flow listings, the property list (`0x1102`, `0x2002`, `0x2010`, `0x2032`, `0x2204`, `0x2320`, `0x2400`, `0x2600`, `0x3300`, `0x3400`, `0x3600`) | **15 of 15** | Every one, byte for byte. |
| Channel counts, device info, property values (`0x1000`, `0x1003`, `0x1100`) | 0 of 6 | Not meant to match: these describe the module itself, and a DM3's is a Broadway where a Rio's is a Brooklyn 3. The emulator gives the Rio's. |

This is a weaker footing than the console's side, and worth being exact
about:

- The layouts are right for one real module. **Nobody has captured Dante
  Controller asking a Rio**, so whether a Rio answers these questions the
  same way, or refuses them and is asked the older ones instead, is unknown.
  The emulator answers both generations, so either should work.
- A receive channel's subscription only has a captured form for "subscribed
  to nothing". A subscription a controller sets therefore shows in the
  first-generation listing and not in the second.
- Half the Rio's property list was never read by the console. Dante
  Controller reads all of it, so the gaps are filled with the DM3 module's
  values. Where both devices were asked the same property they agreed on all
  but one.
- Two of Dante Controller's ConMon queries (`0x0075`, `0x0077`) go
  unanswered: only the DM3's module was seen to answer them. A third,
  `0x0015`, is answered with the Rio's own start-up status.
- And none of it has been tried: see "Tried" in §5 for why that needs a
  second machine.

What that does and does not show:

- It shows the **answers are right**. Wherever no audio is involved, the
  console got from the emulator what it got from the Rio.
- It does not show the **questions are complete**. A console with different
  firmware, or a CL, a PM or a DM7, may ask things the QL1 did not. Those
  get no answer and are logged ("... with no known reply").
- It does not show what the console **does with silence**: an ARC flow list
  that is empty, a subscription that never connects, a device with no clock.

### What is copied without being understood

Honesty about the limits of a capture. These are sent exactly as the Rio
sent them, with no claim about what they mean:

- most of every ConMon status body (they describe the Brooklyn 3 module);
- the mDNS version strings;
- nine words of the ARC channel-count reply, the ARC property list, and the
  values of most ARC properties;
- five single-byte head-amp parameters (`0722/10`, `/12`, `/13`, `/14`,
  `/15`) and four of the per-channel arrays (`/18`, `/19`, `/1a`, `/1b`),
  which a console can write and read back but which drive nothing here;
- one word in the make-and-model status that read `0x1015` in the Rio's
  first second and `0x1017` after. The settled value is sent.

## 5. What only a console can settle

In this order. Each one decides whether the next is worth trying. Capture
the whole session (`tcpdump -i <interface> -w bench.pcap 'not (udp portrange
14336-14591)'`); if anything fails, the capture is what fixes it.

1. **Does the console see it?** With the virtual Rio running, does it appear
   in the console's Dante device list, and in Dante Controller? Watch the
   runner for "ARC request with no known reply". (Dante Controller has to be
   on a different machine from the virtual Rio; see "Tried" below.)
2. **Does the console mount it?** Mount the unit id. Does the console show
   the Rio's head amps, or "not found" / "conflict"? Watch for the poll being
   answered and the console's reads following.
3. **Does a gain move arrive?** Turn a head-amp gain on the console. The
   runner should print `console -> input N: gain …`.
4. **Does it stay mounted?** Leave it ten minutes. A stagebox that drops off
   and comes back means a heartbeat is missing or wrong.
5. **Does the console follow a change from this end?** Run with `--set 3 20`.
   Does the console's display move? A Rio shared by two consoles suggests it
   should, but it was never observed.
6. **Which layers did it need?** Repeat 2 and 3 with `--no-mdns`, then with
   `--mbc-only`. If the head-amp protocol alone is enough, a real device
   could carry the audio while the bridge answers for its head amps.
7. **Audio.** §6.

Also worth an afternoon with a console and a real Rio, because it would let
the emulated one drive more than gain and +48 V: change HPF, HPF frequency,
polarity and pad on one input, one at a time, with a capture running. Each
will move exactly one of the arrays above.

### Tried: Dante Controller on the same Mac (2026-11-09)

Dante Controller 4.18.1, on the machine that was running the virtual Rio.
It is the only real Audinate controller this has met, and it did not get as
far as a test:

- **It finds the device.** Its status bar counted one device and its event
  log named it. The mDNS records are good enough for Audinate's own
  discovery.
- **It asked it nothing.** No ARC query, no ConMon registration and no status
  query reached the virtual Rio, and it never appeared in the routing grid.
  With the Rio on Dante Controller's own address there was no error at all;
  the likely reason (inferred, not confirmed) is that Dante software takes a
  device at its own address to belong to the local Dante stack. With the Rio
  on a second local address, advertised through Bonjour, the event log said
  why: the device's address "does not match the subnet configuration of the
  local Dante interface".
- **It only uses real network ports.** It would not take a virtual link
  (`feth`), so this ran on a physical port.

So a Dante Controller test needs the virtual Rio on a **different machine**
on the same subnet. When this was tried the emulator could not have answered
Dante Controller's own questions anyway (`0x2002`, `0x2320`, `0x2400`,
`0x2600`, `0x3400`, `0x3600`). It answers them now, from the DM3 capture
(§4), so the two-machine test is worth running; it is still the first time
Dante Controller will have seen those answers.

## 6. Audio

The virtual Rio transmits nothing and opens no flows. A console that patches
one of its inputs from the virtual Rio subscribes to a channel that resolves
(the mDNS record is there) and never connects.

So the head-amp control and the audio have to be brought together on the
console some other way. None of these has been tried:

- **Insert return.** Patch the console channel's input from the virtual Rio,
  so its gain control drives the virtual head amp, and bring the real signal
  in on the channel's insert return. Works on paper for mono channels on a
  CL/QL; costs an insert.
- **Dante Controller patching.** Let the console keep its patch to the
  virtual Rio for control, and subscribe the same console input to the real
  transmitter in Dante Controller. Whether the console keeps the head-amp
  link once the subscription points elsewhere is the question.
- **Channel records that point at the real device.** A `_netaudio-chan`
  record says where a channel's audio comes from. The virtual Rio's records
  could name the real transmitter's host and channel, so the console's own
  subscription lands on real audio. Speculative: it depends on how a
  receiver sets a flow up once it has resolved the name.
- **Be an audio device.** Run a clock and relay the audio through the
  virtual Rio. A different and much larger piece of work.

## 7. Adding another device

The split is there for this.

- **A different Dante device** is a [`Persona`](../crates/emulation/src/persona.rs):
  maker, model id, product name, channel counts. The ARC, mDNS and
  make-and-model answers are built from it. The module-level status bodies
  are a Brooklyn 3's; a device on another Dante module wants its own,
  captured the same way.
- **A different control protocol** is a state machine like
  [`mbc::device`](../crates/preamp-adapter-yamaha/src/mbc/device.rs), in
  that vendor's adapter crate, riding on `VirtualDevice::vendor()`. Do not
  put it in `crates/emulation`.

What each candidate still needs:

| Device | Missing |
|---|---|
| Other R-series (Rio1608-D2, Tio1608-D, …) | Their Audinate model ids and whether the head-amp arrays stay 32 wide. A capture of one pairing each. |
| Allen & Heath DT168 / DT164-W | The `AllenHth` vendor body framing is still open ([`allenheath-dt-preamp-over-dante.md`](allenheath-dt-preamp-over-dante.md)). One capture of a DT168 with a console or DT Preamp Control. |
| A Shure or Sennheiser receiver, as the console's own wireless screen shows it | A different protocol altogether (ACN, for Shure on a Yamaha). The setup handshake was never captured. |

## 8. Where things are

| | |
|---|---|
| `crates/emulation` | The virtual Dante device: `mdns`, `arc`, `cmc`, `status`, `device` (sockets), `capture` (pcap reader for the replays). |
| `crates/preamp-adapter-yamaha/src/mbc/device.rs` | The stagebox's half of `MBC`, as a state machine with no sockets. |
| `crates/preamp-adapter-yamaha/src/mbc/emulated.rs` | `EmulatedRioAdapter`: the two put together behind `DeviceAdapter`. |
| `crates/plugin-yamaha-rio-emulated` | Kind `yamaha-rio-emulated`. |
| `examples/emulated_rio.rs`, `replay_mbc_device.rs`, `replay_capture.rs` | The bench runner and the two replays. |
