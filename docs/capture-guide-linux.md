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
| A mirroring switch or tap | Only if you already own one — see [Alternatives](#already-own-a-mirroring-switch-or-a-tap) below. *Optional* |

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

## Already own a mirroring switch or a tap?

Neither is necessary — the laptop-bridge method above works fine on its own — but if
you already have one of these, it's a perfectly good alternative to bridging.

**Managed switch with a mirror / SPAN port** — Plug the desk, stagebox, and your laptop
into the switch as normal, then configure one port to mirror the desk and stagebox's
traffic onto your laptop's port. Capture directly on that single NIC — no bridging
needed. *Needs admin access to the switch.*

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

---

Part of the Dante-BabelBox emulation research. A finished `.pcapng` file is all we
need — send it over as-is.
