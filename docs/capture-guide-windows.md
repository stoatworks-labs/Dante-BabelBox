# Watching Two Boxes Talk — Windows Edition

*Field guide — network capture.* A step-by-step guide to recording the private
conversation between a mixing desk and its stagebox — using a laptop, two network
cables, and one free piece of software. No specialist network gear required.

No tap needed · No mirror switch needed · ~20 minutes · Bench setup only

Also available: [macOS edition](capture-guide-macos.md) · [Linux edition](capture-guide-linux.md)

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
| Wireshark, installed | Free at [wireshark.org](https://www.wireshark.org) — Npcap, its packet driver, installs automatically alongside it. **Required** |
| A mirroring switch or tap | Only if you already own one — see [Alternatives](#already-own-a-mirroring-switch-or-a-tap) below. *Optional* |

## The setup, step by step

### 1. Wire it up

Run a cable from the desk's Ethernet port to **NIC A** on your laptop. Run a second
cable from **NIC B** on your laptop to the stagebox's Ethernet port. That's it — no
switch in between, your laptop *is* the connection now.

### 2. Bridge your two network ports

Tell your laptop to join the two ports into one silent pass-through, so nothing changes
on the wire — just Wireshark gets to watch.

Open **Control Panel → Network Connections**, click your first adapter, `Ctrl`-click
the second, right-click either one, and choose **Bridge Connections**. Windows creates
a new `Network Bridge` adapter for you.

> Select both real adapters, right-click, choose *Bridge Connections*. Leave Wi-Fi out
> of it.

Using a Mac or Linux box instead? There's a matching guide for each — the bridging step
works differently, but everything else here is the same.

### 3. Open Wireshark and pick your interface

Look for `Network Bridge` in Wireshark's interface list — that's the one to capture on.

If nothing shows up on `Network Bridge` once traffic is flowing, select `Ethernet` and
`Ethernet 2` together instead and start both — Wireshark merges them into one capture
automatically.

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
Afterward, undo the bridge: right-click the `Network Bridge` adapter and choose
**Remove from Bridge** on both ports, so your laptop goes back to normal.

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

**I don't see "Bridge Connections" when I right-click.**
Make sure both adapters show as *Enabled* first, and that you've selected both together
(Ctrl-click) before right-clicking — the option only appears with two selected at once.

**The Network Bridge interface shows no traffic.**
Capture on both individual physical adapters at the same time instead — Wireshark
merges them into a single, correctly-ordered capture automatically.

**Windows Firewall seems to be blocking discovery.**
Temporarily allow the traffic through Windows Defender Firewall's private-network
profile, or disable it just for the bench test — remember to turn it back on
afterward.

**The desk and stagebox won't find each other through the laptop.**
Some laptops' Wi-Fi or antivirus network filtering can interfere with the multicast
traffic discovery relies on. Turn Wi-Fi off entirely, or fall back to the
mirror-port/tap method above.

---

Part of the Dante-BabelBox emulation research. A finished `.pcapng` file is all we
need — send it over as-is.
