# Watching Two Boxes Talk — Linux Edition

*Field guide — network capture.* A step-by-step guide to recording the private
conversation between a mixing desk and its stagebox — using a laptop, two network
cables, and one free piece of software. No specialist network gear required.

No tap needed · No mirror switch needed · ~20 minutes · Bench setup only

Also available: [Windows edition](capture-guide-windows.md) · [macOS edition](capture-guide-macos.md)

## The idea, in one picture

Normally, your desk and stagebox talk directly to each other over a single cable (or
through a plain switch). For a few minutes, we're going to put your laptop directly in
the middle of that connection and tell it to quietly pass everything through unchanged
— like fitting a clear length of pipe into a hose. Wireshark then just watches what
flows past.

Every packet that would normally jump straight from desk to stagebox now physically
passes through your laptop first.

## Why we're doing this

Every desk-and-stagebox pair from the same manufacturer has a private handshake — how
they find each other, pair up, and agree on gain and phantom power — that's never been
written down publicly. To build tools that can join that conversation from outside, we
first have to record it happening between two devices that already trust each other.

> **⚠ Bench test, not showtime.** Do this on a spare desk-and-stagebox pair, before or
> after a show — never on a live network mid-event. Inserting anything into a
> production Dante network, even briefly, isn't worth the risk.

## What you'll need

| Item | Notes |
|---|---|
| The desk | Your console. **Required** |
| Its matching stagebox | Same brand and family as the desk — you want the pairing they already trust. **Required** |
| Two Ethernet cables | The ordinary ones you'd use anyway. **Required** |
| A laptop with two network ports | A built-in Ethernet port plus a cheap USB-to-Ethernet adapter covers almost any laptop. **Required** |
| Wireshark, installed | Install via your package manager, e.g. `sudo apt install wireshark`. Say yes when asked to let non-root users capture, then `sudo usermod -aG wireshark $USER` and log back in. **Required** |
| A mirroring switch | Not needed for the method above — but there's now a ~£25 switch that does it, and one situation where mirroring is the *only* way. See [Mirroring a port instead](#mirroring-a-port-instead). *Optional* |

## The setup, step by step

### 1. Wire it up

Run a cable from the desk's Ethernet port to **NIC A** on your laptop. Run a second
cable from **NIC B** on your laptop to the stagebox's Ethernet port. That's it — no
switch in between, your laptop *is* the connection now.

### 2. Bridge your two network ports

Linux does this from a terminal rather than a menu — still just a handful of
copy-pasted lines, no config files to edit. First, find your two interface names with
`ip link show`, then create the bridge and add both interfaces to it:

```sh
$ ip link show
2: enp3s0: <BROADCAST,MULTICAST,UP,LOWER_UP> ...
3: enx00e04c680001: <BROADCAST,MULTICAST,UP,LOWER_UP> ...

# swap these two names for your own from the listing above
$ sudo ip link add name br0 type bridge
$ sudo ip link set enp3s0 master br0
$ sudo ip link set enx00e04c680001 master br0
$ sudo ip link set br0 up
$ sudo ip link set enp3s0 up
$ sudo ip link set enx00e04c680001 up
```

Both interfaces keep working normally — they're just quietly mirrored to each other
through `br0` now.

Using Windows or a Mac instead? There's a matching guide for each — the bridging step
works differently, but everything else here is the same.

### 3. Open Wireshark and pick your interface

Look for `br0` in Wireshark's interface list — that's the one to capture on.

If nothing shows up on `br0` once traffic is flowing, select both physical interfaces
together instead and start both — Wireshark merges them into one capture automatically.

### 4. Start the capture first

Click the blue shark-fin **Start** button *before* powering anything on. This is what
catches the very first discovery packets — the part we're actually missing today.

### 5. Power everything on

Turn the stagebox and desk on in whatever order you'd normally use, then wait about 30
seconds for them to find each other over the network.

### 6. Pair them, like normal

From the desk's own screen, connect to the stagebox exactly as you always would. No
special settings — routine operation is exactly what we want recorded.

### 7. Wiggle a few things

Once, each: nudge a gain knob physically on the stagebox; nudge a different one from
the desk's on-screen control; toggle phantom power on a channel; then disconnect the
desk from the stagebox in software and reconnect it. That re-pairing moment is one of
the most useful bits in the whole capture.

### 8. Stop and save

Hit the red **Stop** square, then **File → Save As**. Give it a name that says what it
is — something like `ql1_tio1608_2026-07-14.pcapng` — and save.

### 9. Send it over, then tidy up

The `.pcapng` file on its own is enough — no need to trim or export anything yourself.
Afterward, delete the bridge; both physical interfaces return to normal on their own:

```sh
$ sudo ip link delete br0 type bridge
```

## Mirroring a port instead

You don't need any of this — the laptop-bridge method above works fine on its own, and
it's still the fastest way to get us a capture. But there are two situations where
mirroring a switch port is the nicer answer, and one where it's the only one:

- **The gear won't tolerate a laptop in the path.** Re-cabling means the stagebox
  vanishes for a few seconds, and not every desk takes that gracefully.
- **You'd rather not touch the audio path at all.** A mirror is passive: desk and
  stagebox stay plugged in exactly where they were, and nothing you do can interrupt
  them.
- **One box is talking privately to another.** A wireless receiver mounted on a console
  unicasts its telemetry straight to that console — an ordinary switch port sees none of
  it, and there may be no cable to insert yourself into. Mirroring is the only way to
  see that traffic at all.

### The cheap route: a UniFi USW-Flex-Mini

Owning a mirroring switch used to mean borrowing something from IT. It doesn't any more.
The **UniFi USW-Flex-Mini** is a palm-sized five-port gigabit switch for about £25 / $30,
and port mirroring is a listed feature — *"operation mode (switching or mirroring) per
port"*, straight off Ubiquiti's own datasheet.

> **⚠ The catch — read this before you buy one.** The Flex Mini has **no web interface of
> its own.** It has to be adopted by a UniFi Network controller before you can configure
> anything: a Dream Machine, a Cloud Key, or the free UniFi Network Application running
> on a Mac, PC, Raspberry Pi or in Docker. If you already run UniFi anywhere, you're two
> minutes from a mirror port. If you don't, standing a controller up is a bigger job than
> bridging your laptop — use the method above instead.

#### 1. Wire it up

Power the switch from its USB-C supply if you have one, which leaves all five ports free
for gear. Port 1 is the only port that accepts PoE in, so if you're powering it over PoE
then port 1 is already spoken for.

| Port | Plug in |
|---|---|
| 2 | The desk |
| 3 | The stagebox |
| 4 | Your laptop — this is the capture port |

Leave your laptop on **Wi-Fi** for talking to the controller, and use its Ethernet port
purely for capture. The warning further down explains why that isn't optional.

#### 2. Turn port 4 into a mirror

In UniFi Network:

1. **Devices** → click the Flex Mini.
2. **Ports** → click **port 4**, the one your laptop is on → **Edit**.
3. Set **Operation** to **Mirroring**.
4. Set **Mirroring Port** to **2** — the desk's port.
5. **Apply Changes**, and give the switch a few seconds to provision.

You're always editing the port the copy comes *out* of, and then naming the port being
copied. Newer UniFi Network versions wrap this in a port profile and move things around,
but it's the same two questions in the same order.

#### 3. Mirror the desk — and one port really is enough

UniFi's mirroring is strictly **one source port to one destination**. There's no "mirror
everything onto port 4" here, and no CLI to sneak around it — the Flex Mini doesn't even
offer SSH.

That turns out not to matter for this job. Mirroring a port copies traffic in **both
directions**, so mirroring port 2 gives you everything the desk sends *and* everything
the stagebox sends back to it. That's the entire conversation, from one port. You'd only
want more if a third device were joining in.

#### 4. Capture

Start Wireshark on your laptop's ordinary Ethernet interface. There's no bridge to make
here — no `br0` to create, and nothing to delete afterwards.

Then follow **steps 4 to 9** of the guide above exactly as written: start the capture
first, power everything on, pair them, wiggle a few things, stop, save, and send the
file over. Only the tidy-up half of step 9 changes — set the port back to switching
rather than deleting a bridge.

#### Two things to expect while it's running

**Your laptop's Ethernet port goes deaf.** A mirror destination only spits copies out —
it won't carry a working network connection. Your laptop can't reach the controller, or
the internet, through that port while mirroring is on. That's correct behaviour, not a
fault, and it's why the controller needs to be reachable over Wi-Fi instead.

**The file gets big, fast.** If Dante *audio* is flowing through port 2, you're copying
every one of those streams too — tens of megabytes per second. The control traffic we're
actually after is a rounding error next to it. Before you start, in Wireshark: **Capture
→ Options → Output**, tick **Create a new file automatically** every **100 MB**, and tick
**Use a ring buffer with 20 files**. You'll finish with the most recent 2 GB instead of a
full disk and a file nobody can open.

#### Put it back afterwards

Set port 4's **Operation** back to **Switching** when you're done. A port left in
mirroring mode looks simply broken to whoever plugs into it next.

### The other two options

**A managed switch you already own** — Any switch with a mirror / SPAN port does the same
job as the Flex Mini above, and most of them will mirror several source ports at once
rather than just one. The menus differ per vendor; the two questions don't. *Needs admin
access to the switch.*

**Hardware network tap** — A small dedicated box that sits inline between desk and
stagebox, just like the laptop-bridge method, and quietly copies every packet out a
third port to your laptop. The most bulletproof option, since it never routes the
devices' whole conversation through a general-purpose computer. *Extra kit to buy — not
required.*

## If something's not working

**"Operation not permitted" when I run the bridge commands.**
Missing `sudo` — every command in Step 2 needs it.

**Wireshark's interface list is empty, or it asks for a password every time.**
You're not in the `wireshark` group yet, or haven't logged out and back in since adding
yourself to it: `sudo usermod -aG wireshark $USER`. To sanity-check capture works at
all in the meantime, run `sudo wireshark` once — just don't make a habit of running it
as root.

**br0 shows no traffic.**
Capture on both individual physical interfaces at the same time instead — Wireshark
merges them into a single, correctly-ordered capture automatically.

**The desk and stagebox won't find each other through the laptop.**
NetworkManager can silently reclaim an interface you've bridged manually — run
`nmcli device set <iface> managed no` on both physical interfaces first. Also check a
local firewall (`ufw` or similar) isn't dropping the multicast traffic discovery relies
on.

**There's no "Mirroring" option on the switch's ports.**
The switch is either not adopted yet, or running old firmware — check for a firmware
update in UniFi Network and let it apply, then look again. If it still isn't there, fall
back to the laptop-bridge method above.

**The mirror port is set up but Wireshark sees nothing.**
Check you edited the *laptop's* port and pointed it at the *desk's* port, not the other
way round — it's an easy one to get backwards, and the wrong way round produces exactly
this. Also check the laptop's Ethernet interface is up at all: some NICs report no link
state worth capturing on when nothing is being sent back to them.

---

Part of the Dante-BabelBox emulation research. A finished `.pcapng` file is all we
need — send it over as-is.
